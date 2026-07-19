//! Lowering the JS [`Inventory`] onto atilla-coding's registration surface.
//!
//! `notes/design.md`: "The inventory of what is loaded — every registered tool,
//! hook, and command, whatever language it came from — lives in Rust: the core
//! registry is the single source of truth, and bindings query it rather than
//! keeping their own lists." This module is the JS binding's half of that: it
//! takes the plane-side [`Inventory`] collected while an extension's factory ran
//! and lowers it onto an
//! [`ExtensionHost`](atilla_coding::core::extensions::registry::ExtensionHost).
//!
//! # What lowers now, and what defers to PR-F
//!
//! Hooks and commands lower onto the host as **stub trait objects**: their
//! `handle` / `run` bodies take the no-op continue path. That is enough to make
//! the registry a faithful *inventory* — `registry.hooks_for(event)`,
//! `registry.commands()`, `registry.implemented_events()` all answer correctly —
//! which is what `notes/design.md`'s implemented-only exposure needs. The live
//! dispatch that actually calls back into the JS handler (the
//! `Affinity::OwnRuntime` rendezvous firing a JS closure and awaiting its
//! outcome) is the `ExtensionRunner` port — PR-F. Until then the stub bodies
//! stand in.
//!
//! Tool lowering defers with it: an atilla-coding
//! [`ToolDefinition`](atilla_coding::core::extensions::types::ToolDefinition)
//! carries an `execute` closure, and a JS tool's `execute` can only run through
//! the same JS-dispatch path. So PR-E records every tool in [`Inventory::tools`]
//! (the metadata a binding needs) but does not synthesize a stub `execute` that
//! would silently swallow calls; that closure is built in PR-F when the dispatch
//! path exists.

use std::sync::Arc;

use atilla_coding::core::extensions::command::{Command, CommandContext};
use atilla_coding::core::extensions::events::ExtensionEvent;
use atilla_coding::core::extensions::hook::{Hook, HookEvent, HookOutcome};
use atilla_coding::core::extensions::registry::ExtensionHost;
use atilla_coding::core::extensions::types::ExtensionContext;

use crate::inventory::Inventory;

/// A hook lowered from JS, standing in until PR-F wires live JS dispatch.
///
/// Its [`handle`](Hook::handle) is the no-op continue path; the real handler
/// closure lives in the `JsRuntime`, keyed by event name.
struct StubHook {
    event: HookEvent,
}

impl Hook for StubHook {
    fn event(&self) -> HookEvent {
        self.event
    }

    fn handle(&self, _event: &mut ExtensionEvent, _ctx: &dyn ExtensionContext) -> HookOutcome {
        // PR-F replaces this with the OwnRuntime rendezvous that fires the JS
        // handler and maps its result to a HookOutcome.
        HookOutcome::Continue
    }
}

/// A command lowered from JS, standing in until PR-F wires live JS dispatch.
struct StubCommand {
    name: String,
}

impl Command for StubCommand {
    fn name(&self) -> &str {
        &self.name
    }

    fn run(&self, _args: &str, _ctx: &dyn CommandContext) -> anyhow::Result<()> {
        // PR-F replaces this with a call into the JS command handler.
        Ok(())
    }
}

/// Lower `inv`'s hooks and commands onto `host` (see the module docs for why
/// tools are recorded but not lowered yet). `_source_path` is the extension's
/// entrypoint, threaded through for the tool/command provenance PR-F will attach.
pub fn lower_inventory(inv: &Inventory, host: &mut dyn ExtensionHost, _source_path: &str) {
    for hook in &inv.hooks {
        // Unknown/not-yet-ported event names are skipped rather than panicking —
        // implemented-only exposure means the registry only carries events the
        // core recognizes.
        if let Some(event) = parse_event(&hook.event) {
            host.register_hook(Arc::new(StubHook { event }));
        }
    }

    for command in &inv.commands {
        host.register_command(Arc::new(StubCommand {
            name: command.name.clone(),
        }));
    }
}

/// Parse a snake_case pi event name into a [`HookEvent`], or `None` if the core
/// does not (yet) recognize it. [`HookEvent`] derives `Deserialize` with
/// snake_case renaming, so this is exactly pi's wire token.
fn parse_event(name: &str) -> Option<HookEvent> {
    serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::{CommandRecord, HookRecord, ToolRecord};
    use atilla_coding::core::extensions::registry::Registry;
    use serde_json::json;

    #[test]
    fn parse_event_recognizes_pi_names_and_rejects_unknown() {
        assert_eq!(parse_event("tool_call"), Some(HookEvent::ToolCall));
        assert_eq!(parse_event("input"), Some(HookEvent::Input));
        assert_eq!(parse_event("session_start"), Some(HookEvent::SessionStart));
        assert_eq!(parse_event("not_a_real_event"), None);
    }

    #[test]
    fn lowers_hooks_and_commands_onto_registry() {
        let inv = Inventory {
            tools: vec![ToolRecord {
                name: "greet".into(),
                label: "Greet".into(),
                description: "say hi".into(),
                parameters: json!({ "type": "object" }),
                ..ToolRecord::default()
            }],
            hooks: vec![
                HookRecord {
                    event: "tool_call".into(),
                },
                HookRecord {
                    event: "input".into(),
                },
                // An unknown event is dropped during lowering.
                HookRecord {
                    event: "totally_made_up".into(),
                },
            ],
            commands: vec![CommandRecord {
                name: "hello".into(),
                description: Some("greet".into()),
            }],
            ..Inventory::default()
        };

        let mut registry = Registry::new();
        inv.lower_onto(&mut registry, "/repo/.pi/extensions/greet.ts");

        // Two recognized hooks landed; the unknown one was skipped.
        assert_eq!(registry.hooks().len(), 2);
        assert_eq!(registry.hooks_for(HookEvent::ToolCall).count(), 1);
        assert_eq!(registry.hooks_for(HookEvent::Input).count(), 1);
        assert!(registry.has_hooks_for(HookEvent::ToolCall));

        // The command landed under its registered name.
        assert_eq!(registry.commands().len(), 1);
        assert_eq!(registry.commands()[0].name(), "hello");

        // Tools are recorded in the inventory but not lowered onto the registry
        // in PR-E (their JS-backed execute needs PR-F's dispatch path).
        assert!(registry.tools().is_empty());
    }
}
