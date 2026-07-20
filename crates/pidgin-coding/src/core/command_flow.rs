//! The command-flow state-machine seam used by the package manager port.
//!
//! pi's `DefaultPackageManager` reaches the outside world through three private
//! runners — `runCommand`, `runCommandCapture`, and `runCommandSync`. Its
//! `package-manager.test.ts` suite (the 43-site command-mock cohort) spies those
//! runners and asserts the exact argv (and, where present, `cwd` / `timeoutMs` /
//! `env`) planned for each operation.
//!
//! Rather than spawn processes, the Rust port expresses each operation as a
//! [`CommandFlowMachine`]: a pure state machine that *plans* the next command to
//! run and consumes the [`CommandOutput`] the host produced, exactly mirroring
//! pi's `await runCommand*(...)` control flow. A host shim (the napi
//! `CommandCore` binding, landed by the steward later) drives a machine by
//! running each planned [`CommandRequest`] and feeding the result back:
//!
//! ```text
//! let mut step = machine.start();
//! while let CommandStep::Run { request } = step {
//!     let output = run_command(request);   // the JS `runCommand(program, args, cwd)`
//!     step = machine.advance(output);
//! }
//! // step is CommandStep::Done { result }
//! ```
//!
//! Phase/state lives in the machine's `&mut self`, the same way pi's OAuth
//! machine and the `FauxCore` seam machines carry their state. One-shot
//! operations (npm install/uninstall) are just machines that emit a single
//! `Run` then `Done`; multi-round operations (the git fetch/reset/clean/install
//! reconcile, the npm version-probe-then-maybe-install flow, the
//! upstream→ls-remote resolution) thread output back through `advance`.

// straitjacket-allow-file:duplication — every operation is a faithful parallel
// `CommandFlowMachine` impl (the `start`/`advance` skeleton, one per pi runner);
// the clone detector reads these mirrored machine bodies here and across the
// `package_manager` submodules as duplicates, but the 1:1 map to pi's runners is
// the point — factoring them would obscure it.

use pidgin_ai::seams::subprocess::{CommandOutput, CommandRequest};
use serde_json::Value;

/// One step of a command flow: either a command the host must run, or the
/// finished result.
///
/// `Done` carries the machine's operation-specific output erased to a
/// [`serde_json::Value`] so a single object-safe `dyn CommandFlowMachine` can
/// hold every operation and hand its result across the napi JSON boundary
/// unchanged. One-shots that plan a command and nothing more finish with
/// [`Value::Null`]; typed operations serialize their result into pi's return
/// shape (see the machines in [`crate::core::package_manager`]).
#[derive(Debug, Clone, PartialEq)]
pub enum CommandStep {
    /// The host must run `request` and feed the [`CommandOutput`] back through
    /// [`CommandFlowMachine::advance`].
    Run {
        /// The command to execute.
        request: CommandRequest,
    },
    /// The flow is complete; `result` is the operation's serialized output.
    Done {
        /// The operation-specific result, serialized to pi's return shape.
        result: Value,
    },
}

/// A pure state machine that plans external commands and consumes their output.
///
/// Mirrors pi's private `runCommand*` call chains: [`start`](Self::start) plans
/// the first command, and each [`advance`](Self::advance) consumes one
/// [`CommandOutput`] and plans the next command (or finishes). Once a machine
/// returns [`CommandStep::Done`], further calls keep returning `Done`.
///
/// The result carried by [`CommandStep::Done`] is a [`serde_json::Value`], which
/// keeps the trait object-safe: a single `Box<dyn CommandFlowMachine>` can hold
/// any of pi's operations regardless of their native return type.
pub trait CommandFlowMachine {
    /// Plan the first command (or finish immediately for a no-op).
    fn start(&mut self) -> CommandStep;

    /// Consume the output of the command last planned and plan the next one.
    fn advance(&mut self, output: CommandOutput) -> CommandStep;
}

/// A machine that plans exactly one command and then finishes with `()`.
///
/// The building block for pi's one-shot runners — `install`, `uninstall`, the
/// git `clean`/`reset` steps — where the argv is fully determined up front and
/// the result is discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneShotCommand {
    request: Option<CommandRequest>,
}

impl OneShotCommand {
    /// Wrap a single [`CommandRequest`] as a one-shot flow.
    pub fn new(request: CommandRequest) -> Self {
        Self {
            request: Some(request),
        }
    }
}

impl CommandFlowMachine for OneShotCommand {
    fn start(&mut self) -> CommandStep {
        match self.request.take() {
            Some(request) => CommandStep::Run { request },
            None => CommandStep::Done {
                result: Value::Null,
            },
        }
    }

    fn advance(&mut self, _output: CommandOutput) -> CommandStep {
        CommandStep::Done {
            result: Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The erased `CommandStep` makes the trait object-safe so a single
    // `CommandCore` can hold every operation as a `dyn CommandFlowMachine`.
    #[test]
    fn command_flow_machine_is_object_safe() {
        let machine = OneShotCommand::new(CommandRequest::new("git", ["status"]));
        let mut boxed: Box<dyn CommandFlowMachine> = Box::new(machine);
        assert!(matches!(boxed.start(), CommandStep::Run { .. }));
    }

    #[test]
    fn one_shot_runs_once_then_done() {
        let mut machine = OneShotCommand::new(CommandRequest::new("git", ["status"]));
        match machine.start() {
            CommandStep::Run { request } => {
                assert_eq!(request, CommandRequest::new("git", ["status"]));
            }
            CommandStep::Done { .. } => panic!("expected a Run step"),
        }
        assert_eq!(
            machine.advance(CommandOutput::ok("")),
            CommandStep::Done {
                result: Value::Null
            }
        );
    }
}
