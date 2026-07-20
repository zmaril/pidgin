//! The extension registry: the single Rust inventory of everything extensions
//! register, and the [`ExtensionHost`] surface they register through.
//!
//! From `notes/design.md` §Extensions: "The inventory of what is loaded — every
//! registered tool, hook, and command, whatever language it came from — lives in
//! Rust: the core registry is the single source of truth, and bindings query it
//! rather than keeping their own lists." [`ExtensionHost`] is the Rust successor
//! to the `pi` object's registration methods (`registerTool` / `on` /
//! `registerCommand`); [`Registry`] is the inventory those calls populate.
//!
//! # Implemented-only exposure
//!
//! design.md's hook-exposure policy is *implemented-only*: a binding exposes
//! exactly the hook events the core has actually implemented, and the surface
//! grows with the port. [`Registry::implemented_events`] realizes the runtime
//! side of that policy — it reports exactly the events that loaded extensions
//! have registered handlers for, which is the inventory a binding queries rather
//! than advertising a fixed list of stub events.
//!
//! Types and inventory only — no dispatch (`emit*`) is wired here; that lands
//! with the `ExtensionRunner` port.
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/{types,runner}.ts`.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::command::Command;
use super::hook::{Hook, HookEvent};
use super::types::ToolDefinition;

/// The registration surface handed to an extension — the Rust successor to pi's
/// `pi` object (`ExtensionAPI`, `types.ts:1167`) for the register-side methods.
///
/// Each binding (the embedded JS plane, or a host-language binding) hands the
/// host language a proxy whose `registerTool` / `on` / `registerCommand` calls
/// land here. Kept to the three core registration kinds this PR covers; provider
/// / renderer / flag / shortcut registration land with their ports.
pub trait ExtensionHost {
    /// Register a tool the LLM can call (pi's `registerTool`, `types.ts:1220`).
    fn register_tool(&mut self, tool: ToolDefinition);

    /// Register an event hook (pi's `on(event, handler)`, `types.ts:1172`).
    fn register_hook(&mut self, hook: Arc<dyn Hook>);

    /// Register a slash command (pi's `registerCommand`, `types.ts`).
    fn register_command(&mut self, command: Arc<dyn Command>);
}

/// The core inventory of everything extensions have registered.
///
/// The single source of truth described in design.md: tools, hooks, and commands
/// from every language live here in registration order, and bindings query this
/// rather than keeping their own lists.
#[derive(Clone, Default)]
pub struct Registry {
    tools: Vec<ToolDefinition>,
    hooks: Vec<Arc<dyn Hook>>,
    commands: Vec<Arc<dyn Command>>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// All registered tools, in registration order.
    pub fn tools(&self) -> &[ToolDefinition] {
        &self.tools
    }

    /// All registered hooks, in registration order.
    pub fn hooks(&self) -> &[Arc<dyn Hook>] {
        &self.hooks
    }

    /// All registered commands, in registration order.
    pub fn commands(&self) -> &[Arc<dyn Command>] {
        &self.commands
    }

    /// The hooks registered for a given event, in registration order.
    ///
    /// This is the ordering pi's `runner.emit` relies on — hooks for one event
    /// run in load order.
    pub fn hooks_for(&self, event: HookEvent) -> impl Iterator<Item = &Arc<dyn Hook>> {
        self.hooks.iter().filter(move |h| h.event() == event)
    }

    /// Whether any hook is registered for the given event.
    pub fn has_hooks_for(&self, event: HookEvent) -> bool {
        self.hooks.iter().any(|h| h.event() == event)
    }

    /// The set of events that loaded extensions have registered handlers for —
    /// the implemented-only inventory a binding exposes (see the module docs).
    pub fn implemented_events(&self) -> BTreeSet<HookEvent> {
        self.hooks.iter().map(|h| h.event()).collect()
    }
}

impl ExtensionHost for Registry {
    fn register_tool(&mut self, tool: ToolDefinition) {
        self.tools.push(tool);
    }

    fn register_hook(&mut self, hook: Arc<dyn Hook>) {
        self.hooks.push(hook);
    }

    fn register_command(&mut self, command: Arc<dyn Command>) {
        self.commands.push(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extensions::events::ExtensionEvent;
    use crate::core::extensions::hook::HookOutcome;
    use crate::core::extensions::types::ExtensionContext;

    struct FixedHook(HookEvent);
    impl Hook for FixedHook {
        fn event(&self) -> HookEvent {
            self.0
        }
        fn handle(&self, _event: &mut ExtensionEvent, _ctx: &dyn ExtensionContext) -> HookOutcome {
            HookOutcome::Continue
        }
    }

    #[test]
    fn registry_collects_hooks_in_order_and_reports_inventory() {
        let mut registry = Registry::new();
        assert!(registry.implemented_events().is_empty());

        registry.register_hook(Arc::new(FixedHook(HookEvent::ToolCall)));
        registry.register_hook(Arc::new(FixedHook(HookEvent::Input)));
        registry.register_hook(Arc::new(FixedHook(HookEvent::ToolCall)));

        // Two hooks on tool_call, one on input, in registration order.
        assert_eq!(registry.hooks().len(), 3);
        assert_eq!(registry.hooks_for(HookEvent::ToolCall).count(), 2);
        assert_eq!(registry.hooks_for(HookEvent::Input).count(), 1);
        assert_eq!(registry.hooks_for(HookEvent::SessionStart).count(), 0);

        assert!(registry.has_hooks_for(HookEvent::ToolCall));
        assert!(!registry.has_hooks_for(HookEvent::SessionStart));

        // The implemented-only inventory reports exactly the two registered
        // events, not the full 33-event surface.
        let implemented = registry.implemented_events();
        assert_eq!(implemented.len(), 2);
        assert!(implemented.contains(&HookEvent::ToolCall));
        assert!(implemented.contains(&HookEvent::Input));
        assert!(!implemented.contains(&HookEvent::SessionStart));
    }

    #[test]
    fn extension_host_register_methods_populate_inventory() {
        let mut registry = Registry::new();
        registry.register_command(Arc::new(NamedCommand("greet")));
        registry.register_command(Arc::new(NamedCommand("bye")));

        assert_eq!(registry.commands().len(), 2);
        assert_eq!(registry.commands()[0].name(), "greet");
        assert_eq!(registry.commands()[1].name(), "bye");
        assert!(registry.tools().is_empty());
    }

    struct NamedCommand(&'static str);
    impl Command for NamedCommand {
        fn name(&self) -> &str {
            self.0
        }
        fn run(
            &self,
            _args: &str,
            _ctx: &dyn crate::core::extensions::command::CommandContext,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }
}
