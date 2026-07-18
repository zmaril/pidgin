//! The subprocess / command-runner seam: injectable external-command execution.
//!
//! # What this abstracts in pi
//!
//! The mock-seam inventory (`notes/mock-inventory.md`) identifies this as the
//! single highest-leverage seam beyond the original four: it collapses 44 mock
//! sites, 43 of which are one coherent suite — `package-manager.test.ts` — that
//! spies `DefaultPackageManager`'s private command runners (`runCommand`,
//! `runCommandCapture`, `runCommandSync`, and the git/npm helpers) to steer
//! results or assert the exact argv on the subprocess boundary. The 44th mocks
//! `child_process` for a git `symbolic-ref` branch lookup. A single injectable
//! command runner reaches all of them.
//!
//! # Implementations
//!
//! - [`SystemCommandRunner`] — the production runner: real `std::process::Command`
//!   execution. This is what ships.
//! - [`ScriptedCommandRunner`] — the deterministic test runner: a queue of
//!   canned [`CommandOutput`] replies, matched to invocations, that also records
//!   every argv it was asked to run so a test can assert on the command line
//!   exactly as pi's `package-manager` tests do.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

/// A command to run: program plus arguments, optionally in a working directory.
///
/// Mirrors the `(command, args, options)` shape pi passes to its private
/// `runCommand*` helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    /// The program to execute (pi's `command`).
    pub program: String,
    /// Positional arguments (pi's `args`).
    pub args: Vec<String>,
    /// Working directory, if pinned (pi's `options.cwd`).
    pub cwd: Option<String>,
}

impl CommandRequest {
    /// Build a request for `program` with `args`, no explicit cwd.
    pub fn new(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
        }
    }

    /// Set the working directory.
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }
}

/// The captured result of running a command: exit status plus captured streams.
///
/// Mirrors what pi's `runCommandCapture` returns (`{ stdout, stderr, code }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Process exit code; `None` when the process was signalled.
    pub code: Option<i32>,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

impl CommandOutput {
    /// A successful run (`code 0`) with the given stdout and empty stderr.
    pub fn ok(stdout: impl Into<String>) -> Self {
        Self {
            code: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// Whether the command exited zero.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Runs external commands, the boundary pi's package manager and git helpers sit
/// behind.
///
/// Production code depends on `&dyn CommandRunner` so a test can inject
/// [`ScriptedCommandRunner`] and both steer results and assert argv, reproducing
/// pi's `package-manager.test.ts` spies without spawning a real process.
pub trait CommandRunner: Send + Sync {
    /// Run `request` to completion, capturing stdout/stderr (pi's
    /// `runCommandCapture`).
    fn run_capture(&self, request: &CommandRequest) -> io::Result<CommandOutput>;
}

/// The production command runner: real process execution.
#[derive(Debug, Default, Clone)]
pub struct SystemCommandRunner;

impl SystemCommandRunner {
    /// Construct the production runner.
    pub fn new() -> Self {
        Self
    }
}

impl CommandRunner for SystemCommandRunner {
    fn run_capture(&self, request: &CommandRequest) -> io::Result<CommandOutput> {
        let mut command = std::process::Command::new(&request.program);
        command.args(&request.args);
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        let output = command.output()?;
        Ok(CommandOutput {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[derive(Default)]
struct ScriptedState {
    replies: VecDeque<io::Result<CommandOutput>>,
    calls: Vec<CommandRequest>,
}

/// A deterministic, scripted command runner for tests.
///
/// Queue replies with [`ScriptedCommandRunner::push_ok`] /
/// [`ScriptedCommandRunner::push_output`]; each [`CommandRunner::run_capture`]
/// pops the next reply and records the request. [`ScriptedCommandRunner::calls`]
/// returns every argv seen, so a test asserts on the command line the way pi's
/// package-manager suite asserts on its spied runner. Running out of scripted
/// replies is itself an assertion failure surfaced as an error.
#[derive(Clone, Default)]
pub struct ScriptedCommandRunner {
    state: Arc<Mutex<ScriptedState>>,
}

impl ScriptedCommandRunner {
    /// An empty scripted runner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a successful reply with `stdout` (exit 0, empty stderr).
    pub fn push_ok(&self, stdout: impl Into<String>) -> &Self {
        self.push_output(Ok(CommandOutput::ok(stdout)))
    }

    /// Queue an arbitrary reply (a non-zero exit, an error, custom streams).
    pub fn push_output(&self, output: io::Result<CommandOutput>) -> &Self {
        self.state.lock().unwrap().replies.push_back(output);
        self
    }

    /// Every request run so far, in order — for argv assertions.
    pub fn calls(&self) -> Vec<CommandRequest> {
        self.state.lock().unwrap().calls.clone()
    }
}

impl CommandRunner for ScriptedCommandRunner {
    fn run_capture(&self, request: &CommandRequest) -> io::Result<CommandOutput> {
        let mut state = self.state.lock().unwrap();
        state.calls.push(request.clone());
        state.replies.pop_front().unwrap_or_else(|| {
            Err(io::Error::other(format!(
                "ScriptedCommandRunner: no scripted reply for `{} {}`",
                request.program,
                request.args.join(" ")
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripted_runner_replays_and_records_argv() {
        let runner = ScriptedCommandRunner::new();
        runner.push_ok("v1.2.3\n").push_output(Ok(CommandOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "boom".to_string(),
        }));

        let first = runner
            .run_capture(&CommandRequest::new("git", ["rev-parse", "HEAD"]))
            .unwrap();
        assert!(first.success());
        assert_eq!(first.stdout, "v1.2.3\n");

        let second = runner
            .run_capture(&CommandRequest::new("npm", ["install"]).with_cwd("/repo"))
            .unwrap();
        assert!(!second.success());
        assert_eq!(second.stderr, "boom");

        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], CommandRequest::new("git", ["rev-parse", "HEAD"]));
        assert_eq!(calls[1].cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn scripted_runner_errors_when_unscripted() {
        let runner = ScriptedCommandRunner::new();
        let err = runner
            .run_capture(&CommandRequest::new("git", ["status"]))
            .unwrap_err();
        assert!(err.to_string().contains("no scripted reply"));
    }

    #[test]
    fn system_runner_executes_a_real_process() {
        let out = SystemCommandRunner::new()
            .run_capture(&CommandRequest::new("printf", ["hello"]))
            .expect("printf runs");
        assert!(out.success());
        assert_eq!(out.stdout, "hello");
    }
}
