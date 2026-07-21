//! The synchronous, non-emitting queries of the [`ExtensionRunner`] seam:
//! `has_handlers` / `get_command` / `get_registered_commands` /
//! `get_all_registered_tools` / `get_flag_values`, built from the per-extension
//! [`Inventory`](crate::inventory::Inventory).
//!
//! Source of truth: `vendor/pi/packages/coding-agent/src/core/extensions/runner.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::anyhow;
use serde_json::{json, Value};

use pidgin_agent::types::AgentToolResult;
use pidgin_coding::core::extensions::command::{RegisteredCommand, ResolvedCommand};
use pidgin_coding::core::extensions::hook::HookEvent;
use pidgin_coding::core::extensions::runner::{FlagValue, RegisteredTool};
use pidgin_coding::core::extensions::types::ToolDefinition;
use pidgin_coding::core::source_info::{SourceInfo, SourceOrigin, SourceScope};

use crate::inventory::{CommandRecord, ToolRecord};
use crate::runtime::JsPlaneHandle;

use super::{block_on_off_ambient, DenoExtensionRunner};

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
        let plane = self.inner.plane_arc();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut tools = Vec::new();
        for extension in self.inventories() {
            for record in &extension.inventory.tools {
                if seen.insert(record.name.clone()) {
                    tools.push(RegisteredTool {
                        tool: tool_definition(record, Arc::clone(&plane)),
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
        let plane = self.inner.plane_arc();
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
                command: registered_command(extension_path, command, Arc::clone(&plane)),
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
/// carried faithfully; the JS-backed `execute` now dispatches through the shared
/// one-shot invoke-stored primitive
/// ([`JsPlaneHandle::invoke_stored`](crate::runtime::JsPlaneHandle::invoke_stored)),
/// invoking the tool's live `execute` closure (kept in `reg.tools[name]`) with
/// `[id, args]` and shaping its JSON result back into an [`AgentToolResult`].
// TODO(unit5): map `execution_mode` / `render_shell` from the record's string
// fields, and thread the `signal` / `on_update` / `ctx` through the primitive.
fn tool_definition(record: &ToolRecord, plane: Arc<JsPlaneHandle>) -> ToolDefinition {
    let tool_name = record.name.clone();
    ToolDefinition {
        name: record.name.clone(),
        label: record.label.clone(),
        description: record.description.clone(),
        parameters: record.parameters.clone(),
        execution_mode: None,
        execute: Arc::new(move |id, args, _signal, _on_update, _ctx| {
            // Invoke the stored JS `execute` closure with positional `[id, args]`.
            let call_args = json!([id, args]);
            let outcome =
                block_on_off_ambient(plane.invoke_stored("tool", tool_name.clone(), &call_args));
            match outcome {
                Ok(invocation) if invocation.ok => serde_json::from_value::<AgentToolResult>(
                    invocation.result,
                )
                .unwrap_or_else(|error| {
                    tool_error_result(format!(
                        "tool '{tool_name}' returned an unparseable result: {error}"
                    ))
                }),
                Ok(invocation) => tool_error_result(invocation.error.unwrap_or_else(|| {
                    format!("tool '{tool_name}' execute threw with no message")
                })),
                Err(error) => {
                    tool_error_result(format!("tool '{tool_name}' invocation failed: {error}"))
                }
            }
        }),
        prepare_arguments: None,
        prompt_snippet: record.prompt_snippet.clone(),
        prompt_guidelines: record.prompt_guidelines.clone(),
        render_shell: None,
        render_call: None,
        render_result: None,
    }
}

/// The error-details [`AgentToolResult`] used when a tool `execute` invocation
/// fails or returns an unparseable shape (mirrors pi surfacing the failure as a
/// tool result rather than unwinding the loop).
fn tool_error_result(message: String) -> AgentToolResult {
    AgentToolResult {
        content: Vec::new(),
        details: json!({ "error": message }),
        added_tool_names: None,
        terminate: None,
    }
}

/// Build a [`RegisteredCommand`] from an inventory [`CommandRecord`]. The JS
/// command handler now dispatches through the shared one-shot invoke-stored
/// primitive, invoking the command's live `handler` closure (kept in
/// `reg.commands[name]`) with the raw `[args]` string; a throw surfaces as an
/// `Err`.
// TODO(unit5): populate `get_argument_completions`, and thread `ctx` through the
// primitive once the command context surface crosses the plane.
fn registered_command(
    extension_path: &str,
    record: &CommandRecord,
    plane: Arc<JsPlaneHandle>,
) -> RegisteredCommand {
    let command_name = record.name.clone();
    RegisteredCommand {
        name: record.name.clone(),
        source_info: synthetic_source_info(extension_path),
        description: record.description.clone(),
        get_argument_completions: None,
        handler: Arc::new(move |args, _ctx| {
            let call_args = json!([args]);
            let outcome = block_on_off_ambient(plane.invoke_stored(
                "command",
                command_name.clone(),
                &call_args,
            ));
            match outcome {
                Ok(invocation) if invocation.ok => Ok(()),
                Ok(invocation) => Err(anyhow!(invocation.error.unwrap_or_else(|| {
                    format!("command '{command_name}' handler threw with no message")
                }))),
                Err(error) => Err(anyhow!(
                    "command '{command_name}' invocation failed: {error}"
                )),
            }
        }),
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
