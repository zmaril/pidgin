// straitjacket-allow-file[:duplication] — a faithful transcription of pi's
// Claude-Code tool-name table plus `splitDeferredTools`
// (`utils/deferred-tools.ts`) and `convertTools` (`anthropic-messages.ts`). The
// ordered-map bookkeeping and per-tool object assembly mirror pi verbatim; the
// clone detector may read the small scaffolding as duplicative by design.
//! Tool-name normalization, deferred-tool splitting, and tool serialization,
//! ported from pi-ai's `packages/ai/src/api/anthropic-messages.ts` and
//! `packages/ai/src/utils/deferred-tools.ts` at pinned commit `3da591ab`.
//!
//! A pi `Tool` is `{ name, description, parameters }`; here it stays an opaque
//! [`serde_json::Value`] (matching the ported [`crate::types::Context`] whose
//! `tools` is `Vec<Value>`), so tools round-trip and serialize exactly as pi's.

use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::types::Message;

/// Claude Code 2.x tool names in canonical casing (`anthropic-messages.ts:79`).
const CLAUDE_CODE_TOOLS: [&str; 17] = [
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "KillShell",
    "NotebookEdit",
    "Skill",
    "Task",
    "TaskOutput",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

/// Convert a tool name to Claude Code canonical casing if it matches
/// case-insensitively, mirroring pi's `toClaudeCodeName`
/// (`anthropic-messages.ts:102`).
pub fn to_claude_code_name(name: &str) -> String {
    let lower = name.to_lowercase();
    CLAUDE_CODE_TOOLS
        .into_iter()
        .find(|canonical| canonical.to_lowercase() == lower)
        .map(str::to_string)
        .unwrap_or_else(|| name.to_string())
}

/// Apply pi's per-request tool-name normalization: Claude-Code casing under
/// OAuth, identity otherwise (`anthropic-messages.ts:929`).
pub fn normalize_tool_name(name: &str, is_oauth: bool) -> String {
    if is_oauth {
        to_claude_code_name(name)
    } else {
        name.to_string()
    }
}

/// Read a tool's `name` field (opaque `Value`), defaulting to `""` as pi's
/// property access would yield for a malformed tool.
fn tool_name(tool: &Value) -> &str {
    tool.get("name").and_then(Value::as_str).unwrap_or("")
}

/// The result of [`split_deferred_tools`]: the immediately-declared tools and
/// the transcript-loaded (deferred) tools, keyed by normalized name in insertion
/// order.
#[derive(Debug, Clone, Default)]
pub struct ToolPlacement {
    pub immediate: Vec<Value>,
    /// Deferred tools as `(normalized_name, tool)` pairs, preserving pi's
    /// `Map` insertion order.
    pub deferred: Vec<(String, Value)>,
}

/// Split current tools into prefix (immediate) and transcript-loaded (deferred)
/// definitions, mirroring pi's `splitDeferredTools`
/// (`utils/deferred-tools.ts:8`). `normalize_name` is applied via
/// [`normalize_tool_name`] with the given `is_oauth` flag.
pub fn split_deferred_tools(
    tools: &[Value],
    messages: &[Message],
    enabled: bool,
    is_oauth: bool,
) -> ToolPlacement {
    // uniqueTools: keyed by normalized name, later entries overwrite the value
    // but keep the first-seen position (JS `Map` semantics).
    let mut order: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    for tool in tools {
        let key = normalize_tool_name(tool_name(tool), is_oauth);
        if let Some(pos) = order.iter().position(|k| k == &key) {
            values[pos] = tool.clone();
        } else {
            order.push(key);
            values.push(tool.clone());
        }
    }

    if !enabled {
        return ToolPlacement {
            immediate: values,
            deferred: Vec::new(),
        };
    }

    let mut deferred_names: HashSet<String> = HashSet::new();
    let mut used_names: HashSet<String> = HashSet::new();
    for message in messages {
        match message {
            Message::Assistant(assistant) => {
                for block in &assistant.content {
                    if let crate::types::ContentBlock::ToolCall { name, .. } = block {
                        used_names.insert(normalize_tool_name(name, is_oauth));
                    }
                }
            }
            Message::ToolResult(result) => {
                for name in result.added_tool_names.iter().flatten() {
                    let normalized = normalize_tool_name(name, is_oauth);
                    if !used_names.contains(&normalized) {
                        deferred_names.insert(normalized);
                    }
                }
            }
            Message::User(_) => {}
        }
    }

    let mut immediate = Vec::new();
    let mut deferred = Vec::new();
    for (name, tool) in order.into_iter().zip(values.into_iter()) {
        if deferred_names.contains(&name) {
            deferred.push((name, tool));
        } else {
            immediate.push(tool);
        }
    }
    ToolPlacement {
        immediate,
        deferred,
    }
}

/// Serialize tools into Anthropic `tools[]` entries, mirroring pi's
/// `convertTools` (`anthropic-messages.ts:1260`). `cache_control` is stamped
/// only on the final tool; `eager_input_streaming` and `defer_loading` are
/// added per their flags.
pub fn convert_tools(
    tools: &[Value],
    is_oauth: bool,
    supports_eager_tool_input_streaming: bool,
    cache_control: Option<&Value>,
    defer_loading: bool,
) -> Vec<Value> {
    let len = tools.len();
    tools
        .iter()
        .enumerate()
        .map(|(index, tool)| {
            let properties = tool
                .get("parameters")
                .and_then(|p| p.get("properties"))
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            let required = tool
                .get("parameters")
                .and_then(|p| p.get("required"))
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));

            let mut entry = Map::new();
            entry.insert(
                "name".to_string(),
                json!(normalize_tool_name(tool_name(tool), is_oauth)),
            );
            // pi emits `description: tool.description`; an undefined description
            // is dropped by `JSON.stringify`, so omit it when absent/null.
            if let Some(description) = tool.get("description").filter(|d| !d.is_null()) {
                entry.insert("description".to_string(), description.clone());
            }
            if supports_eager_tool_input_streaming {
                entry.insert("eager_input_streaming".to_string(), json!(true));
            }
            entry.insert(
                "input_schema".to_string(),
                json!({ "type": "object", "properties": properties, "required": required }),
            );
            if defer_loading {
                entry.insert("defer_loading".to_string(), json!(true));
            }
            if let Some(cache_control) = cache_control {
                if index == len - 1 {
                    entry.insert("cache_control".to_string(), cache_control.clone());
                }
            }
            Value::Object(entry)
        })
        .collect()
}
