//! The synchronous, non-emitting queries of the [`ExtensionRunner`] seam:
//! `has_handlers` / `get_command` / `get_registered_commands` /
//! `get_all_registered_tools` / `get_flag_values`, built from the per-extension
//! [`Inventory`](crate::inventory::Inventory).
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/runner.ts`.

// straitjacket-allow-file:duplication -- the command-collision resolution mirrors
// pi's `resolveRegisteredCommands` and the tool/flag folds mirror
// `getAllRegisteredTools` / `getFlagValues`; the structure is faithful to the
// ported source.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{json, Value};

use atilla_agent::types::AgentToolResult;
use atilla_coding::core::extensions::command::{RegisteredCommand, ResolvedCommand};
use atilla_coding::core::extensions::hook::HookEvent;
use atilla_coding::core::extensions::runner::{FlagValue, RegisteredTool};
use atilla_coding::core::extensions::types::ToolDefinition;
use atilla_coding::core::source_info::{SourceInfo, SourceOrigin, SourceScope};

use crate::inventory::{CommandRecord, ToolRecord};

use super::DenoExtensionRunner;

/// The `&str` -> [`HookEvent`] adapter the seam contract calls for: match the raw
/// event-type string against every [`HookEvent`] by its `as_str` name. Returns
/// `None` for a name with no enum member yet (pi's opaque dispatch events).
pub fn hook_event_from_str(name: &str) -> Option<HookEvent> {
    HookEvent::ALL
        .into_iter()
        .find(|event| event.as_str() == name)
}

impl DenoExtensionRunner {
    /// `hasHandlers` (`runner.ts:565`): true if any loaded extension registered a
    /// handler for `event_type`. Routes recognized event names through the
    /// [`HookEvent`] adapter and falls back to a raw-name lookup for the opaque
    /// dispatch events that have no enum member.
    pub(crate) fn query_has_handlers(&self, event_type: &str) -> bool {
        match hook_event_from_str(event_type) {
            Some(event) => self.inner.has_handlers(event),
            None => !self.inner.sites_by_name(event_type).is_empty(),
        }
    }

    /// `getAllRegisteredTools` (`runner.ts:447`): every registered tool paired
    /// with its provenance, deduped by tool name (first registration wins).
    pub(crate) fn query_all_registered_tools(&self) -> Vec<RegisteredTool> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut tools = Vec::new();
        for extension in self.inventories() {
            for record in &extension.inventory.tools {
                if seen.insert(record.name.clone()) {
                    tools.push(RegisteredTool {
                        tool: tool_definition(record),
                        source_info: synthetic_source_info(&extension.path),
                    });
                }
            }
        }
        tools
    }

    /// `getFlagValues` (`runner.ts:486`): the current value of every registered
    /// flag as a `boolean | string`, later registrations overriding earlier ones.
    pub(crate) fn query_flag_values(&self) -> BTreeMap<String, FlagValue> {
        let mut values = BTreeMap::new();
        for extension in self.inventories() {
            for flag in &extension.inventory.flags {
                if let Some(value) = flag
                    .value
                    .clone()
                    .or_else(|| flag.default.clone())
                    .and_then(flag_value)
                {
                    values.insert(flag.name.clone(), value);
                }
            }
        }
        values
    }

    /// `getRegisteredCommands` (`runner.ts:635`) via `resolveRegisteredCommands`:
    /// flatten every extension's commands and disambiguate name collisions with
    /// an `:N` occurrence suffix.
    pub(crate) fn query_registered_commands(&self) -> Vec<ResolvedCommand> {
        let mut records: Vec<(&str, &CommandRecord)> = Vec::new();
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for extension in self.inventories() {
            for command in &extension.inventory.commands {
                records.push((extension.path.as_str(), command));
                *counts.entry(command.name.as_str()).or_insert(0) += 1;
            }
        }

        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        let mut taken: BTreeSet<String> = BTreeSet::new();
        let mut resolved = Vec::with_capacity(records.len());

        for (extension_path, command) in records {
            let occurrence = {
                let entry = seen.entry(command.name.as_str()).or_insert(0);
                *entry += 1;
                *entry
            };
            let mut invocation_name = if counts.get(command.name.as_str()).copied().unwrap_or(0) > 1
            {
                format!("{}:{}", command.name, occurrence)
            } else {
                command.name.clone()
            };
            if taken.contains(&invocation_name) {
                let mut suffix = occurrence;
                loop {
                    suffix += 1;
                    invocation_name = format!("{}:{}", command.name, suffix);
                    if !taken.contains(&invocation_name) {
                        break;
                    }
                }
            }
            taken.insert(invocation_name.clone());
            resolved.push(ResolvedCommand {
                command: registered_command(extension_path, command),
                invocation_name,
            });
        }
        resolved
    }

    /// `getCommand(name)` (`runner.ts:644`): the resolved command whose
    /// invocation name equals `name`.
    pub(crate) fn query_get_command(&self, name: &str) -> Option<ResolvedCommand> {
        self.query_registered_commands()
            .into_iter()
            .find(|command| command.invocation_name == name)
    }
}

/// Build a synthetic [`SourceInfo`] attributing a resource to the extension at
/// `path` (the orchestrator re-stamps provenance later; this is the load-time
/// placeholder pi's `RegisteredTool.sourceInfo` carries).
fn synthetic_source_info(path: &str) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: path.to_string(),
        scope: SourceScope::Project,
        origin: SourceOrigin::TopLevel,
        base_dir: None,
    }
}

/// Lower an inventory [`ToolRecord`] into a [`ToolDefinition`]. The metadata is
/// carried faithfully; the JS-backed `execute` dispatch is not wired here (no
/// acceptance path invokes a registered tool through the runner yet), so the
/// synthesized `execute` returns an error-details result.
// TODO(unit5): back `execute` with a JS tool-invocation primitive on the plane
// and map `execution_mode` / `render_shell` from the record's string fields.
fn tool_definition(record: &ToolRecord) -> ToolDefinition {
    let tool_name = record.name.clone();
    ToolDefinition {
        name: record.name.clone(),
        label: record.label.clone(),
        description: record.description.clone(),
        parameters: record.parameters.clone(),
        execution_mode: None,
        execute: Arc::new(
            move |_id, _args, _signal, _on_update, _ctx| AgentToolResult {
                content: Vec::new(),
                details: json!({
                    "error": format!(
                        "tool '{tool_name}' has no host-backed execute yet (deno runner impl)"
                    ),
                }),
                added_tool_names: None,
                terminate: None,
            },
        ),
        prepare_arguments: None,
        prompt_snippet: record.prompt_snippet.clone(),
        prompt_guidelines: record.prompt_guidelines.clone(),
        render_shell: None,
    }
}

/// Build a [`RegisteredCommand`] from an inventory [`CommandRecord`]. The JS
/// command handler is not wired (no acceptance path runs a registered command
/// through the runner yet), so the synthesized handler is a no-op.
// TODO(unit5): back `handler` with a JS command-invocation primitive on the
// plane, and populate `get_argument_completions`.
fn registered_command(extension_path: &str, record: &CommandRecord) -> RegisteredCommand {
    RegisteredCommand {
        name: record.name.clone(),
        source_info: synthetic_source_info(extension_path),
        description: record.description.clone(),
        get_argument_completions: None,
        handler: Arc::new(|_args, _ctx| Ok(())),
    }
}

/// Map a flag's JSON value to the seam's `boolean | string` [`FlagValue`],
/// dropping any other JSON shape (pi's flag values are only boolean or string).
fn flag_value(value: Value) -> Option<FlagValue> {
    match value {
        Value::Bool(boolean) => Some(FlagValue::Bool(boolean)),
        Value::String(string) => Some(FlagValue::Str(string)),
        _ => None,
    }
}
