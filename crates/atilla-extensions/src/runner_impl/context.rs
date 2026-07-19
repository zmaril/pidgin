//! `createCommandContext` (`runner.ts:736`): the concrete [`CommandContext`] the
//! runner mints for a command invocation.
//!
//! The seam models `CommandContext` as an opaque marker trait (its capability
//! members — `args`, `flags`, session controls — are deferred to the full
//! extension-context port), so this concrete carries the args/flags snapshot read
//! from the bound [`ExtensionCommandContextHost`] and otherwise satisfies the
//! marker.

use std::collections::BTreeMap;

use atilla_coding::core::extensions::command::CommandContext;
use atilla_coding::core::extensions::runner::FlagValue;
use atilla_coding::core::extensions::types::ExtensionContext;

use super::DenoExtensionRunner;

/// The concrete [`CommandContext`] returned by
/// [`create_command_context`](atilla_coding::core::extensions::runner::ExtensionRunner::create_command_context).
///
/// Holds the args/flags snapshot from the bound command-context host; the fields
/// are captured now so a later port can expose them through the widened
/// `CommandContext` surface.
pub(crate) struct DenoCommandContext {
    #[allow(dead_code)] // Captured for the deferred `CommandContext.args` accessor.
    args: String,
    #[allow(dead_code)] // Captured for the deferred `CommandContext.flags` accessor.
    flags: BTreeMap<String, FlagValue>,
}

impl ExtensionContext for DenoCommandContext {}
impl CommandContext for DenoCommandContext {}

impl DenoExtensionRunner {
    /// `createCommandContext` (`runner.ts:736`): snapshot the bound
    /// command-context host's args/flags (empty when none is bound) into a fresh
    /// [`DenoCommandContext`].
    pub(crate) fn make_command_context(&self) -> Box<dyn CommandContext> {
        let bindings = self.bindings.lock().unwrap();
        let (args, flags) = match &bindings.command_context_host {
            Some(host) => (host.get_args(), host.get_flags()),
            None => (String::new(), BTreeMap::new()),
        };
        Box::new(DenoCommandContext { args, flags })
    }
}
