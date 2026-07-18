//! Shared test helpers for the package-manager machine tests.

use crate::core::command_flow::{CommandFlowMachine, CommandStep};
use atilla_ai::seams::subprocess::{CommandOutput, CommandRequest};
use serde_json::Value;

pub(crate) fn s(items: &[&str]) -> Vec<String> {
    items.iter().map(|i| i.to_string()).collect()
}

/// Drive a machine to completion, returning every planned request in order
/// plus the final serialized result. Each scripted output is fed in sequence.
pub(crate) fn drive<M: CommandFlowMachine>(
    machine: &mut M,
    outputs: Vec<CommandOutput>,
) -> (Vec<CommandRequest>, Value) {
    let mut requests = Vec::new();
    let mut outputs = outputs.into_iter();
    let mut step = machine.start();
    loop {
        match step {
            CommandStep::Run { request } => {
                requests.push(request);
                let output = outputs
                    .next()
                    .expect("machine planned more commands than scripted outputs");
                step = machine.advance(output);
            }
            CommandStep::Done { result } => return (requests, result),
        }
    }
}

pub(crate) fn ok(stdout: &str) -> CommandOutput {
    CommandOutput::ok(stdout)
}

pub(crate) fn fail() -> CommandOutput {
    CommandOutput {
        code: Some(1),
        stdout: String::new(),
        stderr: String::new(),
    }
}
